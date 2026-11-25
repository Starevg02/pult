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
        .routes(routes!(handover))
        .routes(routes!(my_assets))
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
    /// История оборудования: выдача, возврат, передача, правки. Для других объектов — пусто.
    pub events: Vec<AssetEvent>,
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
    let (asset, events) = if kind == "asset" {
        (fetch_asset(&core, id).await?, events_of(&core.db, id).await?)
    } else {
        (None, Vec::new())
    };
    Ok(Json(ObjectCard { kind, id, label, detail, tickets, asset, events }))
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
    /// Состояния типа и разрешённые переходы.
    #[schema(value_type = crate::cmdb::Lifecycle)]
    pub lifecycle: Value,
    /// Объекты типа показываются в «Состоянии сервисов».
    pub show_status: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct AssetTypeInput {
    pub name: String,
    /// Дополнительные поля карточки (как поля формы категории).
    #[serde(default)]
    pub fields: Vec<tickets::FormField>,
    #[serde(default = "yes")]
    pub is_active: bool,
    /// Показывать объекты типа в «Состоянии сервисов». Не передано — без изменений (при создании — нет).
    #[serde(default)]
    pub show_status: Option<bool>,
}

fn yes() -> bool {
    true
}

#[utoipa::path(get, path = "/api/helpdesk/asset-types", tag = "helpdesk", responses((status = 200, body = Vec<AssetType>)))]
async fn list_asset_types(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<AssetType>>> {
    user.require(&perm::WORK)?;
    let rows = sqlx::query_as(
        "SELECT t.id, t.name, t.fields, t.is_active, (SELECT count(*) FROM assets a WHERE a.type_id = t.id) AS count, t.lifecycle, t.show_status
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
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO asset_types (name, fields, is_active, show_status) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(b.name.trim())
    .bind(json!(b.fields))
    .bind(b.is_active)
    .bind(b.show_status.unwrap_or(false))
    .fetch_one(&core.db)
    .await
    .map_err(dup)?;
    audit::record(&core.db, &user.actor, Entry::new("asset_type.create", Source::Portal).object("asset_type", id))
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

pub(crate) fn dup(e: sqlx::Error) -> ApiError {
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
    let n = sqlx::query(
        "UPDATE asset_types SET name = $2, fields = $3, is_active = $4, show_status = coalesce($5, show_status)
          WHERE id = $1",
    )
    .bind(id)
    .bind(b.name.trim())
    .bind(json!(b.fields))
    .bind(b.is_active)
    .bind(b.show_status)
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

/// `old` — прежнее состояние, если тип не меняется: переход проверяется по жизненному циклу типа.
async fn check_asset(core: &Core, b: &AssetInput, old: Option<&str>) -> ApiResult<Value> {
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
    if let Some(l) = crate::cmdb::lifecycle_of(&core.db, b.type_id).await? {
        crate::cmdb::check_status(&l, old, &b.status)?;
    }
    tickets::validate_answers(&fields, &b.attributes).map_err(bad)
}

fn clean(s: &Option<String>) -> Option<String> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(200).collect())
}

// ------------------------------------------------------------------ История оборудования

/// Запись истории: выдача, возврат, передача, правка карточки.
struct NewEvent<'a> {
    asset: Uuid,
    kind: &'a str,
    from: Option<Uuid>,
    to: Option<Uuid>,
    changes: Value,
    comment: Option<String>,
}

/// Имена сотрудников запоминаются в строке истории — карточку человека могут удалить.
async fn record_event(
    tx: &mut Transaction<'_, Postgres>,
    e: NewEvent<'_>,
    actor: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO asset_events (asset_id, kind, from_person_id, from_name, to_person_id, to_name, changes, comment, actor_id)
         VALUES ($1, $2,
                 $3, (SELECT nullif(trim(concat_ws(' ', last_name, first_name, middle_name)), '') FROM persons WHERE id = $3),
                 $4, (SELECT nullif(trim(concat_ws(' ', last_name, first_name, middle_name)), '') FROM persons WHERE id = $4),
                 $5, $6, $7)",
    )
    .bind(e.asset)
    .bind(e.kind)
    .bind(e.from)
    .bind(e.to)
    .bind(e.changes)
    .bind(e.comment)
    .bind(actor)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Вид записи по смене ответственного: был → стал.
fn custody_kind(from: Option<Uuid>, to: Option<Uuid>) -> Option<&'static str> {
    match (from, to) {
        (None, Some(_)) => Some("issued"),
        (Some(_), None) => Some("returned"),
        (Some(a), Some(b)) if a != b => Some("transferred"),
        _ => None,
    }
}

/// Поля карточки, которые сравниваются для истории (ответственный — отдельной записью).
const TRACKED: &[&str] =
    &["type_id", "name", "inventory_number", "serial_number", "site_id", "room", "status", "notes", "attributes"];

/// Изменённые поля: `{"поле": [было, стало]}`.
fn diff(old: &Value, new: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for k in TRACKED {
        let (a, b) = (old.get(*k).unwrap_or(&Value::Null), new.get(*k).unwrap_or(&Value::Null));
        if a != b {
            out.insert((*k).to_string(), json!([a, b]));
        }
    }
    Value::Object(out)
}

#[utoipa::path(post, path = "/api/helpdesk/assets", tag = "helpdesk", request_body = AssetInput, responses((status = 201, body = Object)))]
async fn create_asset(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<AssetInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&MANAGE_ASSETS)?;
    let attrs = check_asset(&core, &b, None).await?;
    let who = Who::from_user(&user);
    let mut tx = core.db.begin().await?;
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
    .fetch_one(&mut *tx)
    .await
    .map_err(dup)?;
    let created = NewEvent {
        asset: id,
        kind: "created",
        from: None,
        to: b.responsible_person_id,
        changes: json!({}),
        comment: None,
    };
    record_event(&mut tx, created, who.user).await?;
    audit::record(&mut *tx, &user.actor, Entry::new("asset.create", Source::Portal).object("asset", id)).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(get, path = "/api/helpdesk/assets/{id}", tag = "helpdesk", params(("id" = Uuid, Path)), responses((status = 200, body = Asset)))]
async fn get_asset(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Asset>> {
    user.require(&perm::WORK)?;
    fetch_asset(&core, id).await?.map(Json).ok_or(ApiError::NotFound)
}

/// Правка карточки. Смена ответственного записывается в историю как выдача, возврат или передача,
/// остальные изменения — одной записью «изменено» с полями «было → стало».
#[utoipa::path(put, path = "/api/helpdesk/assets/{id}", tag = "helpdesk", params(("id" = Uuid, Path)), request_body = AssetInput, responses((status = 204)))]
async fn update_asset(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<AssetInput>,
) -> ApiResult<StatusCode> {
    user.require(&MANAGE_ASSETS)?;
    let prev: Option<(Uuid, String)> =
        sqlx::query_as("SELECT type_id, status FROM assets WHERE id = $1").bind(id).fetch_optional(&core.db).await?;
    let prev = prev.ok_or(ApiError::NotFound)?;
    let attrs = check_asset(&core, &b, (prev.0 == b.type_id).then_some(prev.1.as_str())).await?;
    let who = Who::from_user(&user);
    let mut tx = core.db.begin().await?;
    let old: Option<(Value, Option<Uuid>)> = sqlx::query_as(
        "SELECT jsonb_build_object('type_id', type_id, 'name', name, 'inventory_number', inventory_number,
                'serial_number', serial_number, 'site_id', site_id, 'room', room, 'status', status, 'notes', notes,
                'attributes', attributes), responsible_person_id
           FROM assets WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let (old, old_person) = old.ok_or(ApiError::NotFound)?;
    let new = json!({
        "type_id": b.type_id, "name": b.name.trim(), "inventory_number": clean(&b.inventory_number),
        "serial_number": clean(&b.serial_number), "site_id": b.site_id, "room": clean(&b.room), "status": b.status,
        "notes": clean(&b.notes), "attributes": attrs,
    });
    sqlx::query(
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
    .bind(&new["attributes"])
    .execute(&mut *tx)
    .await
    .map_err(dup)?;
    let changes = diff(&old, &new);
    if let Some(kind) = custody_kind(old_person, b.responsible_person_id) {
        let e = NewEvent {
            asset: id,
            kind,
            from: old_person,
            to: b.responsible_person_id,
            changes: json!({}),
            comment: None,
        };
        record_event(&mut tx, e, who.user).await?;
    }
    let changed = changes.as_object().is_some_and(|m| !m.is_empty());
    if changed {
        let e = NewEvent { asset: id, kind: "updated", from: None, to: None, changes: changes.clone(), comment: None };
        record_event(&mut tx, e, who.user).await?;
    }
    let details = json!({"changes": changes, "responsible": [old_person, b.responsible_person_id]});
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("asset.update", Source::Portal).object("asset", id).details(details),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct HandoverInput {
    /// `issue` — выдать сотруднику, `return` — вернуть, `transfer` — передать другому сотруднику.
    pub action: String,
    /// Кому выдать или передать.
    pub person_id: Option<Uuid>,
    /// Состояние после возврата: `in_stock` (по умолчанию) или `repair`.
    pub status: Option<String>,
    pub comment: Option<String>,
}

/// Выдать, вернуть или передать оборудование. Запись — в историю оборудования и в журнал аудита.
#[utoipa::path(post, path = "/api/helpdesk/assets/{id}/handover", tag = "helpdesk", params(("id" = Uuid, Path)),
    request_body = HandoverInput, responses((status = 204)))]
async fn handover(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<HandoverInput>,
) -> ApiResult<StatusCode> {
    user.require(&MANAGE_ASSETS)?;
    let who = Who::from_user(&user);
    let bad = |m: &str| ApiError::bad_request("invalid_asset", m.to_string());
    let comment: Option<String> =
        b.comment.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(1000).collect());
    let mut tx = core.db.begin().await?;
    let row: Option<(Option<Uuid>, String, Uuid)> =
        sqlx::query_as("SELECT responsible_person_id, status, type_id FROM assets WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (current, status, type_id) = row.ok_or(ApiError::NotFound)?;
    if status == "retired" {
        return Err(bad_state("Списанное оборудование нельзя выдать, вернуть или передать."));
    }
    let person = || b.person_id.ok_or_else(|| bad("Выберите сотрудника."));
    let (kind, to, new_status) = match (b.action.as_str(), current) {
        ("issue", None) => ("issued", Some(person()?), "in_use".to_string()),
        ("issue", Some(_)) => return Err(bad_state("Оборудование уже выдано — передайте или верните его.")),
        ("transfer", Some(cur)) => {
            let p = person()?;
            if p == cur {
                return Err(bad("Оборудование уже у этого сотрудника."));
            }
            ("transferred", Some(p), "in_use".to_string())
        }
        ("return", Some(_)) => {
            let st = b.status.clone().unwrap_or_else(|| "in_stock".into());
            if !matches!(st.as_str(), "in_stock" | "repair") {
                return Err(bad("Неверное состояние."));
            }
            ("returned", None, st)
        }
        ("transfer" | "return", None) => return Err(bad_state("Оборудование ни за кем не закреплено.")),
        _ => return Err(bad("Действие — issue, return или transfer.")),
    };
    if let Some(l) = crate::cmdb::lifecycle_of(&mut *tx, type_id).await? {
        crate::cmdb::check_status(&l, Some(&status), &new_status)?;
    }
    if let Some(p) = to {
        let active: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM persons WHERE id = $1 AND NOT is_archived)")
                .bind(p)
                .fetch_one(&mut *tx)
                .await?;
        if !active {
            return Err(bad("Сотрудник не найден."));
        }
    }
    sqlx::query("UPDATE assets SET responsible_person_id = $2, status = $3, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(to)
        .bind(&new_status)
        .execute(&mut *tx)
        .await?;
    let changes = if new_status == status { json!({}) } else { json!({"status": [status, new_status]}) };
    let e = NewEvent { asset: id, kind, from: current, to, changes, comment };
    record_event(&mut tx, e, who.user).await?;
    let details = json!({"action": kind, "from": current, "to": to, "status": new_status});
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("asset.handover", Source::Portal).object("asset", id).details(details),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct AssetEvent {
    /// `created`, `issued`, `returned`, `transferred`, `updated`.
    pub kind: String,
    pub from_name: Option<String>,
    pub to_name: Option<String>,
    /// Изменённые поля: `{"поле": [было, стало]}`.
    #[schema(value_type = Object)]
    pub changes: Value,
    pub comment: Option<String>,
    /// Кто сделал запись.
    pub actor_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

/// История оборудования, новые записи сверху.
async fn events_of(db: &sqlx::PgPool, asset: Uuid) -> Result<Vec<AssetEvent>, sqlx::Error> {
    sqlx::query_as(
        "SELECT e.kind, e.from_name, e.to_name, e.changes, e.comment, e.created_at,
                coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), a.sam_account_name) AS actor_name
           FROM asset_events e
           LEFT JOIN users u ON u.id = e.actor_id
           LEFT JOIN persons p ON p.id = u.person_id
           LEFT JOIN ad_accounts a ON a.id = u.ad_account_id
          WHERE e.asset_id = $1
          ORDER BY e.created_at DESC, e.id DESC LIMIT 200",
    )
    .bind(asset)
    .fetch_all(db)
    .await
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct MyAsset {
    pub id: Uuid,
    pub name: String,
    pub type_name: String,
    pub inventory_number: Option<String>,
    pub serial_number: Option<String>,
    /// `in_use`, `in_stock`, `repair`.
    pub status: String,
    pub site_name: Option<String>,
    pub room: Option<String>,
    /// Когда выдано этому сотруднику.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub since: Option<OffsetDateTime>,
}

/// Оборудование, закреплённое за текущим сотрудником. Только просмотр: история заявок и
/// карточка — у ИТ.
#[utoipa::path(get, path = "/api/helpdesk/my/assets", tag = "helpdesk", responses((status = 200, body = Vec<MyAsset>)))]
async fn my_assets(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<MyAsset>>> {
    let Some(person) = user.person_id else { return Ok(Json(Vec::new())) };
    let rows = sqlx::query_as(
        "SELECT a.id, a.name, t.name AS type_name, a.inventory_number, a.serial_number, a.status, s.name AS site_name, a.room,
                (SELECT max(e.created_at) FROM asset_events e WHERE e.asset_id = a.id AND e.to_person_id = $1) AS since
           FROM assets a JOIN asset_types t ON t.id = a.type_id
           LEFT JOIN sites s ON s.id = a.site_id
          WHERE a.responsible_person_id = $1 AND a.status <> 'retired'
          ORDER BY t.name, a.name LIMIT 200",
    )
    .bind(person)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
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

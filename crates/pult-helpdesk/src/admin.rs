//! Админка Helpdesk: категории, рабочее время, отсутствия.

use crate::{HelpdeskState, perm, settings::HelpdeskSettings, tickets::FormField};
use axum::{
    Json,
    extract::{FromRef, Path, Query, State},
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
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
    HelpdeskState: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(list_categories))
        .routes(routes!(create_category))
        .routes(routes!(update_category))
        .routes(routes!(get_settings, put_settings))
        .routes(routes!(list_absences, create_absence))
        .routes(routes!(delete_absence))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct Category {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub code: Option<String>,
    pub name: String,
    pub kind: String,
    pub default_priority: String,
    #[schema(value_type = Vec<FormField>)]
    pub form_schema: Value,
    pub is_active: bool,
    pub sort: i32,
    pub version: i32,
}

const CATEGORY_COLS: &str = "id, parent_id, code, name, kind, default_priority, form_schema, is_active, sort, version";

#[derive(Deserialize, IntoParams)]
pub struct CategoryQuery {
    /// С отключёнными — для админки.
    #[serde(default)]
    pub all: bool,
}

/// Категории; служебная «Не указана» в форме не показывается.
#[utoipa::path(get, path = "/api/helpdesk/categories", tag = "helpdesk", params(CategoryQuery), responses((status = 200, body = Vec<Category>)))]
async fn list_categories(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<CategoryQuery>,
) -> ApiResult<Json<Vec<Category>>> {
    if q.all {
        user.require(&perm::ADMIN)?;
    }
    let rows = sqlx::query_as(&format!(
        // SAFE: константа
        "SELECT {CATEGORY_COLS} FROM ticket_categories
          WHERE $1 OR (is_active AND code IS DISTINCT FROM 'unspecified')
          ORDER BY sort, name"
    ))
    .bind(q.all)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize, ToSchema)]
pub struct CategoryInput {
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub kind: String,
    pub default_priority: String,
    #[serde(default)]
    pub form_schema: Vec<FormField>,
    pub is_active: bool,
    pub sort: i32,
    #[serde(default)]
    pub version: i32,
}

fn validate_category(b: &CategoryInput) -> ApiResult<()> {
    let bad = |m: String| ApiError::bad_request("invalid_category", m);
    if b.name.trim().is_empty() {
        return Err(bad("Укажите название.".into()));
    }
    if !matches!(b.kind.as_str(), "incident" | "request")
        || !matches!(b.default_priority.as_str(), "low" | "normal" | "high" | "critical")
    {
        return Err(bad("Неверный тип или приоритет.".into()));
    }
    let mut keys = std::collections::HashSet::new();
    for f in &b.form_schema {
        if f.key.trim().is_empty() || f.label.trim().is_empty() || !keys.insert(&f.key) {
            return Err(bad("У каждого поля формы — свой ключ и подпись.".into()));
        }
        if !matches!(f.kind.as_str(), "text" | "select" | "bool") {
            return Err(bad("Тип поля — text, select или bool.".into()));
        }
        if f.kind == "select" && f.options.is_empty() {
            return Err(bad(format!("Укажите варианты поля «{}».", f.label)));
        }
    }
    Ok(())
}

/// Дерево — не глубже трёх уровней и без циклов.
async fn check_parent(core: &Core, id: Option<Uuid>, parent: Option<Uuid>) -> ApiResult<()> {
    let Some(parent) = parent else { return Ok(()) };
    let depth: Option<(i32, bool)> = sqlx::query_as(
        "WITH RECURSIVE up AS (
             SELECT id, parent_id, 1 AS depth FROM ticket_categories WHERE id = $1
             UNION ALL SELECT c.id, c.parent_id, up.depth + 1 FROM ticket_categories c JOIN up ON c.id = up.parent_id WHERE up.depth < 10
         ) SELECT max(depth), coalesce(bool_or(id = $2), false) FROM up HAVING count(*) > 0",
    )
    .bind(parent)
    .bind(id)
    .fetch_optional(&core.db)
    .await?;
    let (depth, cycle) = depth.ok_or_else(|| ApiError::bad_request("invalid_category", "Родитель не найден."))?;
    if cycle || depth >= 3 {
        return Err(ApiError::bad_request("invalid_category", "Категории — не глубже трёх уровней и без циклов."));
    }
    Ok(())
}

#[utoipa::path(post, path = "/api/admin/helpdesk/categories", tag = "admin", request_body = CategoryInput, responses((status = 200, body = Category)))]
async fn create_category(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<CategoryInput>,
) -> ApiResult<Json<Category>> {
    user.require(&perm::ADMIN)?;
    validate_category(&b)?;
    check_parent(&core, None, b.parent_id).await?;
    let c: Category = sqlx::query_as(&format!(
        // SAFE: константа
        "INSERT INTO ticket_categories (parent_id, name, kind, default_priority, form_schema, is_active, sort)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {CATEGORY_COLS}"
    ))
    .bind(b.parent_id)
    .bind(b.name.trim())
    .bind(&b.kind)
    .bind(&b.default_priority)
    .bind(json!(b.form_schema))
    .bind(b.is_active)
    .bind(b.sort)
    .fetch_one(&core.db)
    .await?;
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("helpdesk.category.create", Source::Portal)
            .object("category", c.id)
            .details(json!({"name": c.name})),
    )
    .await?;
    Ok(Json(c))
}

#[utoipa::path(put, path = "/api/admin/helpdesk/categories/{id}", tag = "admin", params(("id" = Uuid, Path)), request_body = CategoryInput,
    responses((status = 200, body = Category), (status = 409, body = pult_core::error::ErrorBody)))]
async fn update_category(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<CategoryInput>,
) -> ApiResult<Json<Category>> {
    user.require(&perm::ADMIN)?;
    validate_category(&b)?;
    check_parent(&core, Some(id), b.parent_id).await?;
    let c: Option<Category> = sqlx::query_as(&format!( // SAFE: константа
        "UPDATE ticket_categories SET parent_id = $3, name = $4, kind = $5, default_priority = $6, form_schema = $7, is_active = $8, sort = $9, version = version + 1
          WHERE id = $1 AND version = $2 RETURNING {CATEGORY_COLS}"
    ))
    .bind(id)
    .bind(b.version)
    .bind(b.parent_id)
    .bind(b.name.trim())
    .bind(&b.kind)
    .bind(&b.default_priority)
    .bind(json!(b.form_schema))
    .bind(b.is_active)
    .bind(b.sort)
    .fetch_optional(&core.db)
    .await?;
    let c = c.ok_or(ApiError::Conflict { current: json!({}) })?;
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("helpdesk.category.update", Source::Portal)
            .object("category", c.id)
            .details(json!({"name": c.name, "is_active": c.is_active})),
    )
    .await?;
    Ok(Json(c))
}

#[utoipa::path(get, path = "/api/admin/helpdesk/settings", tag = "admin", responses((status = 200, body = HelpdeskSettings)))]
async fn get_settings(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<HelpdeskSettings>> {
    user.require(&perm::ADMIN)?;
    Ok(Json(settings::load(&core.db).await?))
}

#[utoipa::path(put, path = "/api/admin/helpdesk/settings", tag = "admin", request_body = HelpdeskSettings, responses((status = 200, body = HelpdeskSettings)))]
async fn put_settings(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<HelpdeskSettings>,
) -> ApiResult<Json<HelpdeskSettings>> {
    user.require(&perm::ADMIN)?;
    b.validate().map_err(|m| ApiError::bad_request("invalid_settings", m))?;
    let mut tx = core.db.begin().await?;
    let before: HelpdeskSettings = settings::load(&mut *tx).await?;
    settings::save(&mut *tx, &b, Some(user.user_id)).await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal)
            .object("settings", "helpdesk")
            .details(audit::diff(&json!(before), &json!(b))),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(b))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct Absence {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub starts_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub ends_at: OffsetDateTime,
    pub reason: Option<String>,
}

const ABSENCE_SELECT: &str = "SELECT ab.id, ab.user_id, ab.starts_at, ab.ends_at, ab.reason,
        coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), a.display_name, a.sam_account_name) AS name
   FROM engineer_absences ab JOIN users u ON u.id = ab.user_id JOIN ad_accounts a ON a.id = u.ad_account_id LEFT JOIN persons p ON p.id = u.person_id";

/// Текущие и будущие отсутствия инженеров.
#[utoipa::path(get, path = "/api/admin/helpdesk/absences", tag = "admin", responses((status = 200, body = Vec<Absence>)))]
async fn list_absences(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<Absence>>> {
    user.require(&perm::ADMIN)?;
    let rows = sqlx::query_as(&format!("{ABSENCE_SELECT} WHERE ab.ends_at > now() ORDER BY ab.starts_at")) // SAFE: константа
        .fetch_all(&core.db)
        .await?;
    Ok(Json(rows))
}

#[derive(Deserialize, ToSchema)]
pub struct AbsenceInput {
    pub user_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub starts_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub ends_at: OffsetDateTime,
    pub reason: Option<String>,
}

#[utoipa::path(post, path = "/api/admin/helpdesk/absences", tag = "admin", request_body = AbsenceInput, responses((status = 200, body = Absence)))]
async fn create_absence(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<AbsenceInput>,
) -> ApiResult<Json<Absence>> {
    user.require(&perm::ADMIN)?;
    if b.ends_at <= b.starts_at {
        return Err(ApiError::bad_request("invalid_absence", "Конец должен быть позже начала."));
    }
    let id: Uuid = sqlx::query_scalar("INSERT INTO engineer_absences (user_id, starts_at, ends_at, reason, created_by) VALUES ($1, $2, $3, $4, $5) RETURNING id")
        .bind(b.user_id)
        .bind(b.starts_at)
        .bind(b.ends_at)
        .bind(&b.reason)
        .bind(user.user_id)
        .fetch_one(&core.db)
        .await?;
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("helpdesk.absence.create", Source::Portal).object("user", b.user_id),
    )
    .await?;
    Ok(Json(sqlx::query_as(&format!("{ABSENCE_SELECT} WHERE ab.id = $1")).bind(id).fetch_one(&core.db).await?)) // SAFE: константа
}

#[utoipa::path(delete, path = "/api/admin/helpdesk/absences/{id}", tag = "admin", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn delete_absence(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let n =
        sqlx::query("DELETE FROM engineer_absences WHERE id = $1").bind(id).execute(&core.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("helpdesk.absence.delete", Source::Portal).object("absence", id))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

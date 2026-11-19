//! Площадки и ответственные инженеры.

use crate::perm;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser,
    audit::{self, Entry, Source},
    rbac::{ROLE_ENGINEER, ROLE_SUPERADMIN},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct Site {
    pub id: Uuid,
    pub name: String,
    pub address: Option<String>,
    pub timezone: String,
    pub emergency_phone: Option<String>,
    pub is_active: bool,
    pub version: i32,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SiteInput {
    pub name: String,
    pub address: Option<String>,
    pub timezone: String,
    #[serde(default)]
    pub emergency_phone: Option<String>,
    pub is_active: bool,
    /// При создании — 0.
    #[serde(default)]
    pub version: i32,
}

async fn validate(core: &Core, s: &SiteInput) -> ApiResult<(String, Option<String>)> {
    let name: String = s.name.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect();
    if name.is_empty() {
        return Err(ApiError::bad_request("invalid_site", "Укажите название площадки."));
    }
    let tz_ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_timezone_names WHERE name = $1)")
        .bind(&s.timezone)
        .fetch_one(&core.db)
        .await?;
    if !tz_ok {
        return Err(ApiError::bad_request("invalid_timezone", format!("Неизвестный часовой пояс: {}", s.timezone)));
    }
    let address = s.address.as_deref().map(str::trim).filter(|a| !a.is_empty()).map(|a| a.chars().take(500).collect());
    Ok((name, address))
}

async fn get(core: &Core, id: Uuid) -> ApiResult<Site> {
    sqlx::query_as("SELECT id, name, address, timezone, emergency_phone, is_active, version FROM sites WHERE id = $1")
        .bind(id)
        .fetch_optional(&core.db)
        .await?
        .ok_or(ApiError::NotFound)
}

/// Площадки (для выбора в справочнике и форме заявки — всем вошедшим).
#[utoipa::path(get, path = "/api/sites", tag = "sites", responses((status = 200, body = Vec<Site>)))]
pub(crate) async fn list_sites(State(core): State<Core>, _user: CurrentUser) -> ApiResult<Json<Vec<Site>>> {
    Ok(Json(
        sqlx::query_as(
            "SELECT id, name, address, timezone, emergency_phone, is_active, version FROM sites ORDER BY is_active DESC, lower(name)",
        )
        .fetch_all(&core.db)
        .await?,
    ))
}

#[utoipa::path(post, path = "/api/admin/sites", tag = "admin", request_body = SiteInput, responses((status = 201, body = Site)))]
pub(crate) async fn create_site(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<SiteInput>,
) -> ApiResult<(StatusCode, Json<Site>)> {
    user.require(&perm::SITES_MANAGE)?;
    let (name, address) = validate(&core, &b).await?;
    let mut tx = core.db.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (name, address, timezone, is_active, emergency_phone) VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(&name)
    .bind(&address)
    .bind(&b.timezone)
    .bind(b.is_active)
    .bind(b.emergency_phone.as_deref().map(str::trim).filter(|p| !p.is_empty()))
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            ApiError::bad_request("name_taken", format!("Площадка «{name}» уже есть."))
        }
        _ => e.into(),
    })?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("site.create", Source::Portal).object("site", id).details(json!({"name": name})),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(get(&core, id).await?)))
}

#[utoipa::path(put, path = "/api/admin/sites/{id}", tag = "admin", params(("id" = Uuid, Path)), request_body = SiteInput,
    responses((status = 200, body = Site), (status = 409)))]
pub(crate) async fn update_site(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<SiteInput>,
) -> ApiResult<Json<Site>> {
    user.require(&perm::SITES_MANAGE)?;
    let (name, address) = validate(&core, &b).await?;
    let before = get(&core, id).await?;
    let mut tx = core.db.begin().await?;
    let n = sqlx::query(
        "UPDATE sites SET name = $3, address = $4, timezone = $5, is_active = $6, emergency_phone = $7, version = version + 1 WHERE id = $1 AND version = $2",
    )
    .bind(id)
    .bind(b.version)
    .bind(&name)
    .bind(&address)
    .bind(&b.timezone)
    .bind(b.is_active)
    .bind(b.emergency_phone.as_deref().map(str::trim).filter(|p| !p.is_empty()))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::Conflict { current: json!(before) });
    }
    let after = json!({"name": name, "address": address, "timezone": b.timezone, "is_active": b.is_active, "emergency_phone": b.emergency_phone});
    let old = json!({"name": before.name, "address": before.address, "timezone": before.timezone, "is_active": before.is_active, "emergency_phone": before.emergency_phone});
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("site.update", Source::Portal).object("site", id).details(audit::diff(&old, &after)),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(get(&core, id).await?))
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct Engineer {
    pub user_id: Uuid,
    pub login: String,
    pub name: String,
    /// `l1` или `l2`.
    pub line: String,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct EngineersInput {
    #[serde(default)]
    pub l1: Vec<Uuid>,
    #[serde(default)]
    pub l2: Vec<Uuid>,
}

async fn engineers(core: &Core, site: Uuid) -> ApiResult<Vec<Engineer>> {
    Ok(sqlx::query_as(
        "SELECT se.user_id, a.sam_account_name AS login,
                coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), a.display_name, a.sam_account_name) AS name,
                se.line
           FROM site_engineers se
           JOIN users u ON u.id = se.user_id
           JOIN ad_accounts a ON a.id = u.ad_account_id
           LEFT JOIN persons p ON p.id = u.person_id
          WHERE se.site_id = $1 ORDER BY se.line, name",
    )
    .bind(site)
    .fetch_all(&core.db)
    .await?)
}

/// Ответственные инженеры площадки.
#[utoipa::path(get, path = "/api/admin/sites/{id}/engineers", tag = "admin", params(("id" = Uuid, Path)),
    responses((status = 200, body = Vec<Engineer>)))]
pub(crate) async fn get_engineers(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<Engineer>>> {
    user.require(&perm::SITES_MANAGE)?;
    get(&core, id).await?;
    Ok(Json(engineers(&core, id).await?))
}

/// Заменить ответственных инженеров L1 и L2 (любое число; только пользователи с ролью «Инженер»).
#[utoipa::path(put, path = "/api/admin/sites/{id}/engineers", tag = "admin", params(("id" = Uuid, Path)),
    request_body = EngineersInput, responses((status = 200, body = Vec<Engineer>)))]
pub(crate) async fn put_engineers(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<EngineersInput>,
) -> ApiResult<Json<Vec<Engineer>>> {
    user.require(&perm::SITES_MANAGE)?;
    get(&core, id).await?;
    let all: Vec<Uuid> = b.l1.iter().chain(&b.l2).copied().collect();
    let ok: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT u.id) FROM users u JOIN user_roles ur ON ur.user_id = u.id JOIN roles r ON r.id = ur.role_id
          WHERE u.id = ANY($1) AND u.is_active AND r.code IN ($2, $3)",
    )
    .bind(&all)
    .bind(ROLE_ENGINEER)
    .bind(ROLE_SUPERADMIN)
    .fetch_one(&core.db)
    .await?;
    let distinct = all.iter().collect::<std::collections::BTreeSet<_>>().len() as i64;
    if ok != distinct {
        return Err(ApiError::bad_request(
            "not_engineer",
            "Ответственным может быть только активный пользователь с ролью «Инженер».",
        ));
    }
    let before = engineers(&core, id).await?;
    let mut tx = core.db.begin().await?;
    sqlx::query("DELETE FROM site_engineers WHERE site_id = $1").bind(id).execute(&mut *tx).await?;
    for (line, ids) in [("l1", &b.l1), ("l2", &b.l2)] {
        sqlx::query("INSERT INTO site_engineers (site_id, user_id, line) SELECT $1, unnest($2::uuid[]), $3 ON CONFLICT DO NOTHING")
            .bind(id)
            .bind(ids)
            .bind(line)
            .execute(&mut *tx)
            .await?;
    }
    let names = |v: &[Engineer]| v.iter().map(|e| format!("{}:{}", e.line, e.login)).collect::<Vec<_>>();
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("site.engineers", Source::Portal)
            .object("site", id)
            .details(json!({"before": names(&before), "after": {"l1": b.l1, "l2": b.l2}})),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(engineers(&core, id).await?))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct CandidateQuery {
    pub q: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct EngineerCandidate {
    pub ad_account_id: Uuid,
    pub login: String,
    pub name: String,
}

/// Кого можно сделать инженером: любая действующая учётка AD, в том числе ещё не входившая в портал.
#[utoipa::path(get, path = "/api/admin/engineers/candidates", tag = "admin", params(CandidateQuery),
    responses((status = 200, body = Vec<EngineerCandidate>)))]
pub(crate) async fn engineer_candidates(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<CandidateQuery>,
) -> ApiResult<Json<Vec<EngineerCandidate>>> {
    user.require(&perm::SITES_MANAGE)?;
    let like = format!("%{}%", q.q.unwrap_or_default().trim().to_lowercase().replace(['%', '_'], ""));
    Ok(Json(
        sqlx::query_as(
            "SELECT a.id AS ad_account_id, a.sam_account_name AS login,
                    coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), a.display_name, a.sam_account_name) AS name
               FROM ad_accounts a LEFT JOIN persons p ON p.ad_account_id = a.id
              WHERE a.is_enabled AND NOT a.is_missing
                AND (lower(a.sam_account_name) LIKE $1 OR lower(coalesce(a.display_name, '')) LIKE $1
                     OR lower(concat_ws(' ', p.last_name, p.first_name)) LIKE $1)
              ORDER BY name LIMIT 20",
        )
        .bind(like)
        .fetch_all(&core.db)
        .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AddEngineer {
    pub ad_account_id: Uuid,
    /// `l1` или `l2`.
    pub line: String,
}

/// Назначить инженером площадки: роль «Инженер» выдаётся здесь же, отдельным шагом это делать не нужно.
#[utoipa::path(post, path = "/api/admin/sites/{id}/engineers", tag = "admin", params(("id" = Uuid, Path)),
    request_body = AddEngineer, responses((status = 200, body = Vec<Engineer>)))]
pub(crate) async fn add_engineer(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<AddEngineer>,
) -> ApiResult<Json<Vec<Engineer>>> {
    user.require(&perm::SITES_MANAGE)?;
    get(&core, id).await?;
    if !matches!(b.line.as_str(), "l1" | "l2") {
        return Err(ApiError::bad_request("invalid_line", "Линия — l1 или l2."));
    }
    let mut tx = core.db.begin().await?;
    let active: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ad_accounts WHERE id = $1 AND is_enabled AND NOT is_missing)")
            .bind(b.ad_account_id)
            .fetch_one(&mut *tx)
            .await?;
    if !active {
        return Err(ApiError::bad_request("not_engineer", "Учётка не найдена или отключена."));
    }
    let target: Uuid = sqlx::query_scalar(
        "INSERT INTO users (ad_account_id, person_id, locale)
         VALUES ($1, (SELECT id FROM persons WHERE ad_account_id = $1), coalesce((SELECT value->>'default_locale' FROM settings WHERE key = 'locale'), 'ru'))
         ON CONFLICT (ad_account_id) DO UPDATE SET ad_account_id = EXCLUDED.ad_account_id RETURNING id",
    )
    .bind(b.ad_account_id)
    .fetch_one(&mut *tx)
    .await?;
    let has_role: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM user_roles ur JOIN roles r ON r.id = ur.role_id WHERE ur.user_id = $1 AND r.code IN ($2, $3))",
    )
    .bind(target)
    .bind(ROLE_ENGINEER)
    .bind(ROLE_SUPERADMIN)
    .fetch_one(&mut *tx)
    .await?;
    if !has_role {
        // Выдача роли — право суперадминистратора: разрешение на площадки не должно давать новых прав.
        if !user.grants.is_superadmin() {
            return Err(ApiError::bad_request(
                "not_engineer",
                "Сначала суперадминистратор должен выдать этому пользователю роль «Инженер».",
            ));
        }
        sqlx::query("INSERT INTO user_roles (user_id, role_id, granted_by) SELECT $1, id, $2 FROM roles WHERE code = $3 ON CONFLICT DO NOTHING")
            .bind(target)
            .bind(user.user_id)
            .bind(ROLE_ENGINEER)
            .execute(&mut *tx)
            .await?;
        audit::record(
            &mut *tx,
            &user.actor,
            Entry::new("user.roles", Source::Portal)
                .object("user", target)
                .details(json!({"added": ROLE_ENGINEER, "via": "site.engineers"})),
        )
        .await?;
    }
    sqlx::query("INSERT INTO site_engineers (site_id, user_id, line) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
        .bind(id)
        .bind(target)
        .bind(&b.line)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("site.engineers", Source::Portal)
            .object("site", id)
            .details(json!({"added": target, "line": b.line})),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(engineers(&core, id).await?))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct LineQuery {
    pub line: String,
}

/// Убрать инженера с линии площадки. Роль остаётся: он может работать на других площадках.
#[utoipa::path(delete, path = "/api/admin/sites/{id}/engineers/{user_id}", tag = "admin",
    params(("id" = Uuid, Path), ("user_id" = Uuid, Path), LineQuery), responses((status = 200, body = Vec<Engineer>)))]
pub(crate) async fn remove_engineer(
    State(core): State<Core>,
    user: CurrentUser,
    Path((id, target)): Path<(Uuid, Uuid)>,
    Query(q): Query<LineQuery>,
) -> ApiResult<Json<Vec<Engineer>>> {
    user.require(&perm::SITES_MANAGE)?;
    let mut tx = core.db.begin().await?;
    sqlx::query("DELETE FROM site_engineers WHERE site_id = $1 AND user_id = $2 AND line = $3")
        .bind(id)
        .bind(target)
        .bind(&q.line)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("site.engineers", Source::Portal)
            .object("site", id)
            .details(json!({"removed": target, "line": q.line})),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(engineers(&core, id).await?))
}

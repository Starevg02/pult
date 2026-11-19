//! HTTP каталога AD: настройки записи (учётка записи, области), операции, состояние учётки.

use crate::{
    ops::{self, AccountView, Kind, NewOperation, OperationView, Target},
    perm,
    zones::DirectorySettings,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Secret,
    audit::{self, Entry, Source},
    settings,
};
use pult_people::{PeopleState, domain};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Настройки записи для админки; пароль учётки записи не возвращается.
#[derive(Debug, Serialize, ToSchema)]
pub struct DirectoryConfig {
    pub user_ous: Vec<String>,
    pub groups: Vec<String>,
    pub require_ticket: bool,
    /// Учётка записи (делегированные права на нужные OU и группы, не Domain Admins).
    pub writer_bind_dn: Option<String>,
    pub has_writer_password: bool,
    /// Домен подключён по LDAPS: без этого сброс пароля невозможен.
    pub ldaps: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DirectoryConfigInput {
    #[serde(default)]
    pub user_ous: Vec<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub require_ticket: bool,
    /// Пусто — запись выключена (учётка записи и её пароль стираются).
    pub writer_bind_dn: Option<String>,
    /// Не передан — пароль не меняется.
    pub writer_password: Option<String>,
}

async fn config(core: &Core) -> ApiResult<DirectoryConfig> {
    let s: DirectorySettings = settings::load(&core.db).await?;
    let dom = domain::current(&core.db).await?;
    let writer: Option<(Option<String>, bool)> = match &dom {
        Some(d) => {
            sqlx::query_as("SELECT writer_bind_dn, writer_secret IS NOT NULL FROM ad_domains WHERE id = $1")
                .bind(d.id)
                .fetch_optional(&core.db)
                .await?
        }
        None => None,
    };
    let (writer_bind_dn, has_writer_password) = writer.unwrap_or((None, false));
    Ok(DirectoryConfig {
        user_ous: s.user_ous,
        groups: s.groups,
        require_ticket: s.require_ticket,
        writer_bind_dn,
        has_writer_password,
        ldaps: dom.is_some_and(|d| d.tls_mode == "ldaps"),
    })
}

#[utoipa::path(get, path = "/api/admin/directory", tag = "directory", responses((status = 200, body = DirectoryConfig)))]
pub(crate) async fn get_directory_config(
    State(core): State<Core>,
    user: CurrentUser,
) -> ApiResult<Json<DirectoryConfig>> {
    user.require(&perm::MANAGE)?;
    Ok(Json(config(&core).await?))
}

#[utoipa::path(put, path = "/api/admin/directory", tag = "directory", request_body = DirectoryConfigInput,
    responses((status = 200, body = DirectoryConfig)))]
pub(crate) async fn put_directory_config(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<DirectoryConfigInput>,
) -> ApiResult<Json<DirectoryConfig>> {
    user.require(&perm::MANAGE)?;
    let Some(dom) = domain::current(&core.db).await? else {
        return Err(ApiError::bad_request("ad_not_configured", "Домен AD не настроен."));
    };
    let s = DirectorySettings { user_ous: b.user_ous, groups: b.groups, require_ticket: b.require_ticket }
        .validate()
        .map_err(|m| ApiError::bad_request("invalid_settings", m))?;
    let writer_dn = b.writer_bind_dn.map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
    if writer_dn.as_deref().is_some_and(|d| d.chars().count() > 500) {
        return Err(ApiError::bad_request("invalid_settings", "Учётка записи — не длиннее 500 символов."));
    }
    let before = config(&core).await?;
    let mut tx = core.db.begin().await?;
    settings::save(&mut *tx, &s, Some(user.user_id)).await?;
    let password_changed = match (&writer_dn, b.writer_password.filter(|p| !p.is_empty())) {
        (None, _) => {
            sqlx::query("UPDATE ad_domains SET writer_bind_dn = NULL, writer_secret = NULL WHERE id = $1")
                .bind(dom.id)
                .execute(&mut *tx)
                .await?;
            before.has_writer_password
        }
        (Some(dn), Some(p)) => {
            let sealed = core.secrets.encrypt(&Secret::new(p)).map_err(ApiError::internal)?;
            sqlx::query("UPDATE ad_domains SET writer_bind_dn = $2, writer_secret = $3 WHERE id = $1")
                .bind(dom.id)
                .bind(dn)
                .bind(sealed)
                .execute(&mut *tx)
                .await?;
            true
        }
        (Some(dn), None) => {
            sqlx::query("UPDATE ad_domains SET writer_bind_dn = $2 WHERE id = $1")
                .bind(dom.id)
                .bind(dn)
                .execute(&mut *tx)
                .await?;
            false
        }
    };
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("directory.settings.update", Source::Portal).object("settings", "directory").details(json!({
            "before": {"user_ous": before.user_ous, "groups": before.groups, "require_ticket": before.require_ticket,
                       "writer_bind_dn": before.writer_bind_dn},
            "after": {"user_ous": s.user_ous, "groups": s.groups, "require_ticket": s.require_ticket,
                      "writer_bind_dn": writer_dn},
            "writer_password_changed": password_changed,
        })),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(config(&core).await?))
}

/// Запрос операции. `confirm` — пользователь подтвердил в интерфейсе.
#[derive(Debug, Deserialize, ToSchema)]
pub struct OperationInput {
    pub kind: Kind,
    /// Сотрудник, чью учётку меняем.
    pub person_id: Uuid,
    /// Для `group.add_member`.
    pub group_dn: Option<String>,
    /// Основание: зачем.
    pub basis: String,
    pub ticket_number: Option<i64>,
    /// Один ключ на одно намерение: повтор запроса не создаёт вторую операцию.
    pub idempotency_key: String,
    #[serde(default)]
    pub confirm: bool,
}

#[utoipa::path(post, path = "/api/directory/operations", tag = "directory", request_body = OperationInput,
    responses((status = 202, body = OperationView)))]
pub(crate) async fn create_operation(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<OperationInput>,
) -> ApiResult<(StatusCode, Json<OperationView>)> {
    if !b.confirm {
        return Err(ApiError::bad_request("confirm_required", "Подтвердите операцию."));
    }
    let ticket_id = match b.ticket_number {
        Some(n) => Some(
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM tickets WHERE number = $1")
                .bind(n)
                .fetch_optional(&core.db)
                .await?
                .ok_or_else(|| ApiError::bad_request("invalid_operation", "Заявка не найдена."))?,
        ),
        None => None,
    };
    let op = ops::request(
        &core,
        &user,
        NewOperation {
            kind: b.kind,
            target: Target::Person(b.person_id),
            group_dn: b.group_dn,
            basis: b.basis,
            ticket_id,
            checklist_item_id: None,
            idempotency_key: b.idempotency_key,
            own_allowed: false,
        },
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(op.localized())))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct OperationsQuery {
    /// Номер заявки.
    pub ticket: i64,
}

#[utoipa::path(get, path = "/api/directory/operations", tag = "directory", params(OperationsQuery),
    responses((status = 200, body = Vec<OperationView>)))]
pub(crate) async fn list_operations(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<OperationsQuery>,
) -> ApiResult<Json<Vec<OperationView>>> {
    user.require(&perm::VIEW)?;
    let Some(ticket) = sqlx::query_scalar::<_, Uuid>("SELECT id FROM tickets WHERE number = $1")
        .bind(q.ticket)
        .fetch_optional(&core.db)
        .await?
    else {
        return Err(ApiError::NotFound);
    };
    Ok(Json(ops::for_ticket(&core.db, ticket).await?.into_iter().map(OperationView::localized).collect()))
}

#[utoipa::path(get, path = "/api/directory/operations/{id}", tag = "directory", params(("id" = Uuid, Path)),
    responses((status = 200, body = OperationView)))]
pub(crate) async fn get_operation(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<OperationView>> {
    let v = ops::view(&core.db, id).await?;
    if v.requested_by != user.user_id {
        user.require(&perm::VIEW)?;
    }
    Ok(Json(v.localized()))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TempPassword {
    pub password: String,
}

/// Временный пароль после сброса — запросившему, один раз.
#[utoipa::path(post, path = "/api/directory/operations/{id}/password", tag = "directory", params(("id" = Uuid, Path)),
    responses((status = 200, body = TempPassword), (status = 410, body = pult_core::error::ErrorBody)))]
pub(crate) async fn reveal_password(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<([(axum::http::HeaderName, &'static str); 1], Json<TempPassword>)> {
    let p = ops::reveal(&core, &user, id).await?;
    // Пароль не должен остаться в кеше браузера или прокси.
    Ok(([(axum::http::header::CACHE_CONTROL, "no-store")], Json(TempPassword { password: p.expose().to_string() })))
}

/// Состояние учётки AD сотрудника: читается из AD сейчас (и сохраняется).
#[utoipa::path(get, path = "/api/directory/persons/{id}/account", tag = "directory", params(("id" = Uuid, Path)),
    responses((status = 200, body = AccountView)))]
pub(crate) async fn person_account(
    State(core): State<Core>,
    State(people): State<PeopleState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AccountView>> {
    user.require(&perm::VIEW)?;
    Ok(Json(ops::account_state(&core, &people.ad, id).await?))
}
